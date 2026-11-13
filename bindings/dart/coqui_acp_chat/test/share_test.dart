// A conversation shown in a second place: the owner keeps the agent, the view follows it and asks through it.
import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:coqui_acp_chat/experimental.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

/// A share of a running conversation with a scripted agent, and a way to open views of it over a link that carries JSON only.
final class Fixture {
  Fixture({int version = 2, Duration interval = Duration.zero}) {
    final made = started(version: version);
    store = made.store;
    session = made.session;
    agent = made.agent;
    session.attachClient('main');
    share = ChatSessionShare.of(session, minInterval: interval);
  }

  late final ChatStore store;
  late final ChatSession session;
  late final FakeConnection agent;
  late final ChatSessionShare share;
  final links = <MemoryViewLink>[];

  Future<ChatSessionView> open(String id, {String? name}) async {
    final (owner, far) = MemoryViewLink.pair();
    links.add(owner);
    share.serve(owner);
    return ChatSessionView.attach(far, clientId: id, name: name);
  }

  Future<void> dispose() async {
    await share.close();
    store.dispose();
  }
}

Map<String, Object?> toolCall(
  String id,
  String title, {
  String status = 'in_progress',
}) => {
  'sessionUpdate': 'tool_call',
  'toolCallId': id,
  'title': title,
  'kind': 'edit',
  'status': status,
};

void main() {
  late Fixture f;
  tearDown(() => f.dispose());

  test(
    'a view attaching late shows the conversation as the owner has it, with its running turn',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      feed(f.session, {
        'sessionUpdate': 'user_message',
        'messageId': 'u1',
        'content': {'type': 'text', 'text': 'Fix the test'},
        '_meta': {chatClientMetaKey: 'main'},
      });
      feed(f.session, text('agent_thought', 'Looking', id: 'th'));
      feed(f.session, toolCall('e', 'Edit calc.js'));
      feed(f.session, text('agent_message_chunk', 'It subtracts.', id: 'a1'));
      feed(f.session, {
        'sessionUpdate': 'plan',
        'entries': [
          {'content': 'Fix', 'status': 'in_progress'},
        ],
      });
      feed(f.session, {
        'sessionUpdate': 'session_info_update',
        'title': 'Calc',
      });

      final view = await f.open('win');
      final shown = view.session;
      expect(shown.messages.map((m) => (m.role, m.text, m.sentBy)), [
        for (final m in f.session.messages) (m.role, m.text, m.sentBy),
      ]);
      expect(shown.toolEntries['e']!.tool!.status, ToolStatus.running);
      expect(shown.plan.single.content, 'Fix');
      expect(shown.title, 'Calc');
      expect(shown.status, ChatStatus.working, reason: 'the turn is running');
      expect(shown.currentTurn, isNotNull);
      expect(shown.agent, f.session.agent);
      expect(shown.clients.map((c) => c.id), unorderedEquals(['main', 'win']));
      expect(f.share.views, ['win']);
    },
  );

  test(
    'what the agent says after a view attached reaches it, and the end of the turn too',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      final view = await f.open('win');
      final seen = <int>[];
      view.session.addListener(() => seen.add(view.session.messages.length));

      feed(f.session, text('agent_message_chunk', 'Hello ', id: 'a'));
      feed(f.session, text('agent_message_chunk', 'there', id: 'a'));
      feed(f.session, toolCall('t', 'Edit'));
      await settle();
      expect(view.session.messages.map((m) => m.text), ['Hello there', '']);
      expect(view.session.toolEntries['t']!.tool!.status, ToolStatus.running);

      feed(f.session, toolCall('t', 'Edit', status: 'completed'));
      feed(f.session, state('idle', reason: 'end_turn'));
      await settle();
      expect(view.session.toolEntries['t']!.tool!.status, ToolStatus.done);
      expect(view.session.status, ChatStatus.connected);
      expect(view.session.currentTurn, isNull);
      expect(view.session.turns.single.outcome, TurnOutcome.completed);
      expect(seen, isNotEmpty);
    },
  );

  test(
    'changes made in a burst reach a view in a few states, not one each',
    () async {
      f = Fixture(interval: const Duration(milliseconds: 60));
      final (owner, far) = MemoryViewLink.pair();
      var states = 0;
      final counted = far.messages.asBroadcastStream();
      counted.listen((m) {
        if (m['t'] == 'state') states++;
      });
      f.share.serve(owner);
      far.send({'t': 'attach', 'v': chatViewWireVersion, 'client': 'win'});
      await settle();
      expect(states, 1);
      for (var i = 0; i < 100; i++) {
        feed(f.session, text('agent_message_chunk', 'x', id: 'a'));
      }
      await Future<void>.delayed(const Duration(milliseconds: 150));
      expect(states, inInclusiveRange(2, 3));
    },
  );

  test(
    'a running turn and a waiting permission survive a pop-out and the dock back',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      feed(f.session, toolCall('t1', 'Edit a.txt', status: 'pending'));
      final answer = f.store.askPermission(request(1), f.agent);
      expect(f.session.pendingPermissions, hasLength(1));

      final popped = await f.open('win');
      expect(popped.session.pendingPermissions.single.title, 'Edit a.txt');
      expect(
        popped.session.pendingPermissions.single.options.map((o) => o.optionId),
        ['allow-once', 'reject'],
      );
      expect(popped.session.status, ChatStatus.working);

      // Dock back: the window goes, the owner goes on as it was.
      await popped.detach();
      await settle();
      expect(f.share.views, isEmpty);
      expect(
        f.session.clients.map((c) => c.id),
        ['main'],
        reason: 'the window is not a client any more',
      );
      expect(
        f.session.pendingPermissions,
        hasLength(1),
        reason: 'it still waits',
      );
      expect(f.session.status, ChatStatus.working);
      expect(f.agent.calls, isEmpty, reason: 'nothing was answered or sent');

      // Popped out again, and the same request is still there to answer, once.
      final again = await f.open('win2');
      expect(
        again.session.pendingPermissions.single.id,
        f.session.pendingPermissions.single.id,
      );
      again.choosePermission(
        again.session.pendingPermissions.single,
        again.session.pendingPermissions.single.options.first,
      );
      await settle();
      expect(
        await answer,
        isA<AcpSelect>().having((s) => s.optionId, 'option', 'allow-once'),
      );
      expect(f.session.settledPermissions.single.outcome!.by, 'win2');
      expect(again.session.pendingPermissions, isEmpty);
    },
  );

  test(
    'an answer in a view settles the owner and the other view, who see who answered',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      final answer = f.store.askPermission(request(1), f.agent);
      final a = await f.open('a', name: 'Window A');
      final b = await f.open('b', name: 'Window B');
      expect(a.session.pendingPermissions, hasLength(1));
      expect(b.session.pendingPermissions, hasLength(1));

      final question = a.session.pendingPermissions.single;
      a.choosePermission(question, question.options.last);
      await settle();

      expect(
        await answer,
        isA<AcpSelect>().having((s) => s.optionId, 'option', 'reject'),
      );
      for (final shown in [f.session, a.session, b.session]) {
        expect(shown.pendingPermissions, isEmpty, reason: shown.id);
        final outcome = shown.settledPermissions.single.outcome!;
        expect(
          (outcome.how, outcome.by),
          (Settlement.answered, 'a'),
          reason: shown.id,
        );
        expect((outcome.answer as AcpSelect).optionId, 'reject');
      }
      expect(b.session.clients.firstWhere((c) => c.id == 'a').name, 'Window A');
    },
  );

  test('an answer in the owner settles every view', () async {
    f = Fixture();
    final answer = f.store.askPermission(request(1), f.agent);
    final a = await f.open('a');
    final b = await f.open('b');
    expect(f.session.answerPermission(allow: true, by: 'main'), isTrue);
    await settle();
    expect(await answer, isA<AcpAllow>());
    for (final view in [a, b]) {
      expect(view.session.pendingPermissions, isEmpty);
      final outcome = view.session.settledPermissions.single.outcome!;
      expect(outcome.by, 'main');
      expect(outcome.answer, isA<AcpAllow>());
    }
  });

  test(
    'of two answers made at once the first wins and the other view is told it was settled',
    () async {
      f = Fixture();
      final answer = f.store.askPermission(request(1), f.agent);
      final a = await f.open('a');
      final b = await f.open('b');
      final qa = a.session.pendingPermissions.single,
          qb = b.session.pendingPermissions.single;
      a.choosePermission(qa, qa.options.first);
      b.choosePermission(qb, qb.options.last);
      await settle();
      expect(
        await answer,
        isA<AcpSelect>().having((s) => s.optionId, 'option', 'allow-once'),
      );
      expect(f.session.settledPermissions, hasLength(1));
      expect(b.session.settledPermissions.single.outcome!.by, 'a');
      expect(b.session.pendingPermissions, isEmpty);
    },
  );

  test(
    'a request the turn ending cancelled is shown as cancelled, not as answered by someone',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      final answer = f.store.askPermission(request(1), f.agent);
      final view = await f.open('win');
      await f.session.cancel(from: 'main');
      await settle();
      expect(await answer, isA<AcpCancel>());
      expect(view.session.pendingPermissions, isEmpty);
      expect(
        view.session.settledPermissions.single.outcome!.answer,
        isA<AcpCancel>(),
      );
    },
  );

  test(
    'an agent\'s question with every kind of field reaches a view, and its answer goes back',
    () async {
      f = Fixture();
      final answer = f.store.askQuestion(
        const AcpElicitation(
          sessionId: 's1',
          message: 'Which?',
          title: 'Pick',
          toolCallId: 't9',
          fields: [
            AcpTextField(
              'note',
              title: 'Note',
              required: true,
              maxLength: 40,
              format: 'email',
              defaultValue: 'a@b.c',
            ),
            AcpChoiceField('one', [
              AcpChoice('x', 'X', description: 'the x'),
            ], defaultValue: 'x'),
            AcpChoicesField(
              'many',
              [AcpChoice('x', 'X'), AcpChoice('y', 'Y')],
              minItems: 1,
              defaultValue: ['y'],
            ),
            AcpNumberField(
              'n',
              integer: true,
              minimum: 0,
              maximum: 9,
              defaultValue: 3,
            ),
            AcpBooleanField('ok', defaultValue: true),
            AcpUnsupportedField('odd', 'object'),
          ],
        ),
        f.agent,
      );
      final view = await f.open('win');
      final shown = view.session.pendingQuestions.single.request;
      expect(
        (shown.message, shown.title, shown.toolCallId),
        ('Which?', 'Pick', 't9'),
      );
      expect(shown.fields.map((x) => x.runtimeType), [
        AcpTextField,
        AcpChoiceField,
        AcpChoicesField,
        AcpNumberField,
        AcpBooleanField,
        AcpUnsupportedField,
      ]);
      final text0 = shown.fields[0] as AcpTextField;
      expect(
        (text0.required, text0.maxLength, text0.format, text0.defaultValue),
        (true, 40, 'email', 'a@b.c'),
      );
      expect((shown.fields[2] as AcpChoicesField).defaultValue, ['y']);
      expect((shown.fields[3] as AcpNumberField).integer, isTrue);
      expect(
        (shown.fields[1] as AcpChoiceField).options.single.description,
        'the x',
      );

      view.answerQuestion(
        view.session.pendingQuestions.single,
        const AcpElicitAccept({'note': 'hi', 'one': 'x'}),
      );
      await settle();
      expect((await answer as AcpElicitAccept).content, {
        'note': 'hi',
        'one': 'x',
      });
      expect(view.session.pendingQuestions, isEmpty);
      expect(view.session.settledQuestions.single.outcome!.by, 'win');
    },
  );

  test(
    'a message sent from a view is the owner\'s: stamped with the window, queued while a turn runs, and sent when it ends',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      final view = await f.open('win');
      await view.send('and then the docs');
      await settle();
      expect(f.session.queue.single.from, 'win');
      expect(view.session.queue.single.text, 'and then the docs');
      expect(f.agent.prompts, isEmpty);

      feed(f.session, state('idle', reason: 'end_turn'));
      await settle();
      expect(f.agent.prompts, ['and then the docs']);
      expect(f.session.messages.last.sentBy, 'win');
      expect(view.session.messages.last.sentBy, 'win');
      expect(view.session.queue, isEmpty);
    },
  );

  test('a send the owner refuses throws in the view', () async {
    f = Fixture();
    feed(f.session, state('running'));
    final view = await f.open('win');
    final limit = const ChatLimits().queued;
    for (var i = 0; i < limit; i++) {
      await view.send('m$i');
    }
    await expectLater(view.send('one too many'), throwsStateError);
  });

  test('Stop in a view ends the running turn', () async {
    f = Fixture();
    feed(f.session, state('running'));
    final view = await f.open('win');
    view.cancel();
    await settle();
    expect(f.agent.cancels, ['s1']);
  });

  test(
    'a conversation that loses its agent is shown disconnected, with what waited withdrawn',
    () async {
      f = Fixture();
      feed(f.session, state('running'));
      f.store.askPermission(request(1), f.agent);
      final view = await f.open('win');
      await f.agent.drop();
      await settle();
      expect(view.session.status, ChatStatus.disconnected);
      expect(view.session.pendingPermissions, isEmpty);
      expect(view.session.connection, isNull);
    },
  );

  test(
    'closing the share ends every view with the reason, and the owner keeps its other clients',
    () async {
      f = Fixture();
      final view = await f.open('win');
      await f.share.close();
      expect(await view.done, 'The conversation is closed');
      expect(view.isOpen, isFalse);
      expect(() => view.cancel(), throwsStateError);
      expect(f.session.clients.map((c) => c.id), ['main']);
    },
  );

  test(
    'a view whose link goes is let go: its client detaches and the owner is told',
    () async {
      f = Fixture();
      final events = <String>[];
      f.share.events.listen((e) => events.add('${e.clientId}:${e.attached}'));
      final (owner, far) = MemoryViewLink.pair();
      f.share.serve(owner);
      final view = await ChatSessionView.attach(far, clientId: 'win');
      await far.close();
      await settle();
      expect(events, ['win:true', 'win:false']);
      expect(f.session.clients.map((c) => c.id), ['main']);
      expect(await view.done, isNull);
    },
  );

  test(
    'a client id in use, a wrong wire version and an owner that is silent are refused or time out',
    () async {
      f = Fixture();
      await f.open('win');
      await expectLater(
        f.open('win'),
        throwsA(
          isA<StateError>().having(
            (e) => e.message,
            'message',
            contains('attached already'),
          ),
        ),
      );
      await expectLater(f.open('bypass'), throwsStateError);

      final (owner, far) = MemoryViewLink.pair();
      f.share.serve(owner);
      final received = far.messages.first;
      far.send({'t': 'attach', 'v': 99, 'client': 'old'});
      expect((await received)['t'], 'refused');

      final (_, mute) = MemoryViewLink.pair();
      await expectLater(
        ChatSessionView.attach(
          mute,
          clientId: 'x',
          timeout: const Duration(milliseconds: 50),
        ),
        throwsA(
          isA<StateError>().having(
            (e) => e.message,
            'message',
            contains('did not answer'),
          ),
        ),
      );
    },
  );

  test(
    'a state that is not one is not applied, and the view stays what it was',
    () async {
      f = Fixture();
      feed(f.session, text('agent_message', 'kept', id: 'a'));
      final (owner, far) = MemoryViewLink.pair();
      final view = ChatSessionView.attach(far, clientId: 'win');
      owner.send({'t': 'state', 'v': 1, 'permissions': 'not a list'});
      await expectLater(view, throwsStateError);
    },
  );

  test(
    'a view\'s own methods that would talk to an agent are not available',
    () async {
      f = Fixture();
      final view = await f.open('win');
      expect(
        () => view.session.connection!.prompt('s1', 'x'),
        throwsUnsupportedError,
      );
    },
  );

  test(
    'a v1 conversation, whose turn has no running state of its own, is shown working while it runs',
    () async {
      f = Fixture(version: 1);
      f.agent.onPrompt = (_) => Completer<String?>().future;
      unawaited(f.session.send('go', from: 'main'));
      await settle();
      final view = await f.open('win');
      expect(view.session.status, ChatStatus.working);
      expect(view.session.messages.single.text, 'go');
      expect(view.session.messages.single.sentBy, 'main');
    },
  );
}
