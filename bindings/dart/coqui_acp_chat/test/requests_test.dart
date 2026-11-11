// What an agent asks of the person: permission requests with the agent's own answers and Bypass, questions as forms, which
// connection a request belongs to, several clients answering (the first wins), and a request whose connection is gone.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

const _all = [allowOnce, allowAlways, rejectAlways, rejectOnce];
const _noAllow = [rejectOnce, rejectAlways];
const _alwaysOnly = [allowAlways, rejectOnce];

AcpElicitation _form(String session, {String message = 'Which one?'}) =>
    AcpElicitation(sessionId: session, message: message, fields: const []);

void main() {
  group("the agent's own answers", () {
    test(
      'wait in the agent order, and the one chosen is what the agent is told',
      () async {
        final (:store, :session, :agent) = started();
        final answer = store.askPermission(request(1, options: _all), agent);
        final question = session.pendingPermissions.single;
        expect(question.options.map((o) => o.optionId), [
          'allow-once',
          'allow-always',
          'reject-always',
          'reject',
        ]);
        expect(question.title, 'Edit a.txt');
        expect(
          question.detail,
          'a.txt',
          reason: 'the first of what it was given, bare',
        );
        expect(
          session.toolEntries['t1']!.tool!.status,
          ToolStatus.pending,
          reason: 'the line shows before the card',
        );
        expect(session.choosePermission(rejectAlways), isTrue);
        expect(
          await answer,
          isA<AcpSelect>().having((s) => s.optionId, 'id', 'reject-always'),
        );
        expect(session.pendingPermissions, isEmpty);
        store.dispose();
      },
    );

    test('an option the request did not offer is not taken', () {
      final (:store, :session, :agent) = started();
      store.askPermission(request(1, options: _noAllow), agent);
      expect(session.choosePermission(allowOnce), isFalse);
      expect(session.pendingPermissions, hasLength(1));
      store.dispose();
    });

    test(
      'allow and refuse answer the oldest, and a request with nothing to choose can only be cancelled',
      () async {
        final (:store, :session, :agent) = started();
        final first = store.askPermission(request(1), agent);
        final second = store.askPermission(
          request(2, options: const []),
          agent,
        );
        expect(session.answerPermission(allow: true), isTrue);
        expect(await first, isA<AcpAllow>());
        expect(session.pendingPermissions.single.options, isEmpty);
        expect(session.cancelPermission(), isTrue);
        expect(await second, isA<AcpCancel>());
        final third = store.askPermission(request(3), agent);
        session.answerPermission(allow: false);
        expect(await third, isA<AcpDeny>());
        expect(
          session.answerPermission(allow: true),
          isFalse,
          reason: 'nothing waits',
        );
        store.dispose();
      },
    );

    test(
      'what it would do shows with all of its input, and the agent\'s reason first',
      () {
        final (:store, :session, :agent) = started();
        store.askPermission(
          AcpPermissionRequest(
            sessionId: 's1',
            toolCallId: 't9',
            title: 'Run a command',
            description: 'To list the files',
            rawInput: {
              'command': 'ls -la',
              'cwd': '/work/demo',
              'timeout': 30,
              'env': {'A': '1'},
            },
            options: const [allowOnce],
          ),
          agent,
        );
        expect(
          session.pendingPermissions.single.detail,
          'To list the files\nls -la\ncwd: /work/demo\ntimeout: 30\nenv: {"A":"1"}',
        );
        store.dispose();
      },
    );
  });

  group('Bypass', () {
    test('is off by default: a request waits and needs the person', () {
      final (:store, :session, :agent) = started();
      store.askPermission(request(1), agent);
      expect(session.pendingPermissions, hasLength(1));
      expect(session.needsYou, isTrue);
      expect(session.inBypass, isFalse);
      store.dispose();
    });

    test(
      'answers allow once with no question, and says so in the conversation',
      () async {
        final (:store, :session, :agent) = started();
        session.setBypass(true);
        final answer = await store.askPermission(
          request(1, options: _all),
          agent,
        );
        expect(
          answer,
          isA<AcpSelect>().having((s) => s.optionId, 'id', 'allow-once'),
        );
        expect(session.pendingPermissions, isEmpty);
        expect(session.messages.last.text, 'Bypass allowed: Edit a.txt (Yes).');
        expect(session.mode, ownBypass);
        expect(session.inBypass, isTrue);
        store.dispose();
      },
    );

    test(
      'answers a request that offers only an always allow, and never one that offers no allow',
      () async {
        final (:store, :session, :agent) = started();
        session.setBypass(true);
        final always = await store.askPermission(
          request(1, options: _alwaysOnly),
          agent,
        );
        expect(
          always,
          isA<AcpSelect>().having((s) => s.optionId, 'id', 'allow-always'),
        );
        store.askPermission(request(2, options: _noAllow), agent);
        expect(
          session.pendingPermissions.single.bypassPassed,
          isTrue,
          reason: 'it was left to them while Bypass was on',
        );
        store.dispose();
      },
    );

    test(
      'answers the requests already waiting when it is chosen, except one with no allow, and says who',
      () async {
        final (:store, :session, :agent) = started();
        final waiting = store.askPermission(request(1), agent);
        store.askPermission(request(2, options: _noAllow), agent);
        session.setBypass(true);
        expect(await waiting, isA<AcpSelect>());
        expect(session.pendingPermissions.map((q) => q.toolCallId), ['t2']);
        expect(session.settledPermissions.single.outcome!.by, bypassClient);
        store.dispose();
      },
    );

    test('is left, and the next request waits', () {
      final (:store, :session, :agent) = started();
      session.setBypass(true);
      session.setBypass(false);
      store.askPermission(request(1), agent);
      expect(session.pendingPermissions, hasLength(1));
      expect(
        session.messages.map((m) => m.text),
        contains('Bypass is off. Requests are asked again.'),
      );
      store.dispose();
    });

    test(
      'never answers a question or a plan for the person, whatever the mode',
      () {
        final (:store, :session, :agent) = started();
        session.setBypass(true);
        store.askPermission(
          request(1, kind: 'switch_mode', rawInput: const {'plan': 'x'}),
          agent,
        );
        store.askPermission(
          request(
            2,
            rawInput: const {
              'questions': [
                {'question': 'Which?'},
              ],
            },
          ),
          agent,
        );
        expect(session.pendingPermissions.map((q) => q.forPerson), [
          true,
          true,
        ]);
        store.dispose();
      },
    );

    test(
      'is not the agent\'s own mode, which asks nothing already: that is shown, and a choice still waiting is not yet in effect',
      () {
        final (:store, :session, :agent) = started();
        session.applyOpened(
          AcpOpenedSession('s1', {
            'modes': {
              'currentModeId': 'manual',
              'availableModes': [
                {'id': 'manual', 'name': 'Manual'},
                {'id': 'bypassPermissions', 'name': 'Bypass'},
              ],
            },
          }),
        );
        expect(session.inBypass, isFalse);
        session.pending['mode'] = const PendingChoice(
          'mode',
          OptionRole.mode,
          'bypassPermissions',
        );
        expect(session.inBypass, isFalse);
        expect(session.mode, 'bypassPermissions', reason: 'shown as chosen');
        expect(session.underlyingMode, 'manual');
        feed(session, {
          'sessionUpdate': 'current_mode_update',
          'currentModeId': 'bypassPermissions',
        });
        expect(session.inBypass, isTrue, reason: 'the agent says it is in it');
        store.askPermission(request(1), agent);
        expect(
          session.pendingPermissions,
          hasLength(1),
          reason: 'the chat answers only under its own Bypass',
        );
        store.dispose();
      },
    );
  });

  group('which conversation a request belongs to', () {
    test(
      'is the one of the connection it came by, whatever session id the agent chose',
      () {
        final store = ChatStore();
        final a = FakeConnection(), b = FakeConnection();
        final one = store.add(title: 'one', agent: 'A', sessionId: 's1')
          ..attach(a);
        final two = store.add(title: 'two', agent: 'B', sessionId: 's1')
          ..attach(b);
        store.askPermission(request(1), b);
        expect(one.pendingPermissions, isEmpty);
        expect(two.pendingPermissions, hasLength(1));
        expect(store.waiting, 1);
        store.dispose();
      },
    );

    test(
      'a request about no conversation here is refused, and a question cancelled',
      () async {
        final (:store, :session, :agent) = started();
        expect(
          await store.askPermission(request(1, session: 'elsewhere'), agent),
          isA<AcpDeny>(),
        );
        expect(
          await store.askPermission(request(1), FakeConnection()),
          isA<AcpDeny>(),
          reason: 'another connection',
        );
        expect(
          await store.askQuestion(_form('elsewhere'), agent),
          isA<AcpElicitCancel>(),
        );
        expect(session.needsYou, isFalse);
        store.dispose();
      },
    );

    test(
      'past the most a conversation holds, the rest are answered cancelled and it says so',
      () async {
        final (:store, :session, :agent) = started(
          limits: const ChatLimits(pendingPermissions: 2, pendingQuestions: 1),
        );
        store.askPermission(request(1), agent);
        store.askPermission(request(2), agent);
        expect(await store.askPermission(request(3), agent), isA<AcpCancel>());
        expect(session.pendingPermissions, hasLength(2));
        expect(
          session.messages.last.text,
          contains('more questions than the 2'),
        );
        store.askQuestion(_form('s1'), agent);
        expect(
          await store.askQuestion(_form('s1'), agent),
          isA<AcpElicitCancel>(),
        );
        store.dispose();
      },
    );
  });

  group('questions', () {
    test(
      'wait for the person, oldest first, and the answer is what the agent is told',
      () async {
        final (:store, :session, :agent) = started();
        final first = store.askQuestion(_form('s1', message: 'one'), agent);
        store.askQuestion(_form('s1', message: 'two'), agent);
        expect(session.pendingQuestions.map((q) => q.request.message), [
          'one',
          'two',
        ]);
        expect(
          session.answerQuestion(const AcpElicitAccept({'a': 'x'})),
          isTrue,
        );
        expect(await first, isA<AcpElicitAccept>());
        expect(session.pendingQuestions.single.request.message, 'two');
        store.dispose();
      },
    );

    test(
      'Stop answers a waiting question cancelled, and a waiting request cancelled as ACP requires',
      () async {
        final (:store, :session, :agent) = started();
        final question = store.askQuestion(_form('s1'), agent);
        final permission = store.askPermission(request(1), agent);
        await session.cancel();
        expect(await question, isA<AcpElicitCancel>());
        expect(await permission, isA<AcpCancel>());
        expect(agent.cancels, ['s1']);
        expect(session.needsYou, isFalse);
        expect(
          session.settledPermissions.single.outcome!.how,
          Settlement.cancelled,
        );
        store.dispose();
      },
    );
  });

  group('several clients', () {
    test(
      'the first answer settles a request, and the others find it settled and send nothing',
      () async {
        final (:store, :session, :agent) = started();
        session
          ..attachClient('desk', name: 'Desktop')
          ..attachClient('phone', name: 'Phone');
        final events = <ChatEvent>[];
        session.events.listen(events.add);
        final answer = store.askPermission(request(1), agent);
        final question = session.pendingPermissions.single;
        expect(
          session.settle(question, const AcpSelect('allow-once'), by: 'phone'),
          isTrue,
        );
        expect(
          session.settle(question, const AcpSelect('reject'), by: 'desk'),
          isFalse,
          reason: 'second',
        );
        expect(
          session.answerPermission(allow: false, by: 'desk'),
          isFalse,
          reason: 'nothing waits',
        );
        expect(
          await answer,
          isA<AcpSelect>().having((s) => s.optionId, 'id', 'allow-once'),
        );
        expect(question.outcome!.by, 'phone');
        expect(question.outcome!.how, Settlement.answered);
        expect(question.isOpen, isFalse);
        await settle();
        expect(
          events.whereType<PermissionSettledEvent>().single.question,
          same(question),
        );
        store.dispose();
      },
    );

    test('a client that is not attached cannot send or answer', () async {
      final (:store, :session, :agent) = started();
      store.askPermission(request(1), agent);
      expect(
        () => session.answerPermission(allow: true, by: 'ghost'),
        throwsStateError,
      );
      await expectLater(session.send('hi', from: 'ghost'), throwsStateError);
      expect(session.pendingPermissions, hasLength(1));
      expect(() => session.attachClient(bypassClient), throwsArgumentError);
      store.dispose();
    });

    test(
      'are bounded, attaching twice is one, and detaching leaves the conversation going',
      () async {
        final (:store, :session, :agent) = started(
          limits: const ChatLimits(clients: 2),
        );
        final events = <ChatEvent>[];
        session.events.listen(events.add);
        final desk = session.attachClient('desk');
        expect(session.attachClient('desk'), same(desk));
        session.attachClient('phone');
        expect(() => session.attachClient('tv'), throwsStateError);
        session.detachClient('phone');
        expect(session.clients.map((c) => c.id), ['desk']);
        await settle();
        expect(
          events.whereType<ClientEvent>().map((e) => (e.client.id, e.attached)),
          [('desk', true), ('phone', true), ('phone', false)],
        );
        store.dispose();
      },
    );

    test(
      'a message is stamped with the client that sent it, whichever way it went',
      () async {
        final (:store, :session, :agent) = started();
        session
          ..attachClient('desk')
          ..attachClient('phone');
        await session.send('from desk', from: 'desk');
        // The agent echoes the next one before the answer to its prompt arrives.
        agent.onPrompt = (text) async {
          feed(session, {
            'sessionUpdate': 'user_message',
            'messageId': 'e1',
            'content': {'type': 'text', 'text': text},
          });
          return 'e1';
        };
        await session.send('from phone', from: 'phone');
        // Another client's message that reached the agent by the hub, stamped in its update.
        feed(session, {
          'sessionUpdate': 'user_message',
          'messageId': 'h1',
          'content': {'type': 'text', 'text': 'from tv'},
          '_meta': {chatClientMetaKey: 'tv'},
        });
        expect(session.messages.map((m) => (m.text, m.sentBy)), [
          ('from desk', 'desk'),
          ('from phone', 'phone'),
          ('from tv', 'tv'),
        ]);
        store.dispose();
      },
    );

    test(
      'a request settled elsewhere is settled here too, with the answer the agent is given',
      () async {
        final (:store, :session, :agent) = started();
        final answer = store.askPermission(request(1), agent);
        expect(
          session.settleElsewhere(
            session.pendingPermissions.single,
            by: 'hub-phone',
            optionId: 'reject',
          ),
          isTrue,
        );
        expect(
          await answer,
          isA<AcpSelect>().having((s) => s.optionId, 'id', 'reject'),
        );
        final other = store.askPermission(request(2), agent);
        session.settleElsewhere(
          session.pendingPermissions.single,
          by: 'hub-phone',
          optionId: 'not-offered',
        );
        expect(await other, isA<AcpCancel>());
        store.dispose();
      },
    );

    test('only the most settled requests are remembered', () {
      final (:store, :session, :agent) = started(
        limits: const ChatLimits(settledRequests: 2),
      );
      for (var i = 1; i <= 3; i++) {
        store.askPermission(request(i), agent);
        session.answerPermission(allow: true);
      }
      expect(session.settledPermissions.map((q) => q.toolCallId), ['t2', 't3']);
      store.dispose();
    });
  });

  group('a request from a connection that is gone', () {
    test(
      'after the agent went away is not shown, its line is stopped, and nobody is told anything',
      () async {
        final (:store, :session, :agent) = started();
        var answered = false;
        store.askPermission(request(11), agent).then((_) => answered = true);
        expect(session.toolEntries['t11']!.tool!.status, ToolStatus.pending);
        final question = session.pendingPermissions.single;
        await agent.drop();
        await settle();
        expect(
          session.pendingPermissions,
          isEmpty,
          reason: 'gone, not disabled',
        );
        expect(session.toolEntries['t11']!.tool!.status, ToolStatus.stopped);
        expect(question.outcome!.how, Settlement.withdrawn);
        expect(session.status, ChatStatus.disconnected);
        expect(session.answerPermission(allow: true), isFalse);
        await settle();
        expect(
          answered,
          isFalse,
          reason: 'nobody was told anything for a request nobody can answer',
        );
        store.dispose();
      },
    );

    test(
      'after the conversation was connected to another agent likewise, and a late answer sends nothing',
      () async {
        final (:store, :session, :agent) = started();
        var answered = false;
        store.askPermission(request(12), agent).then((_) => answered = true);
        final question = session.pendingPermissions.single;
        final again = FakeConnection();
        session.attach(again);
        expect(session.pendingPermissions, isEmpty);
        expect(session.toolEntries['t12']!.tool!.status, ToolStatus.stopped);
        expect(session.settle(question, const AcpAllow()), isFalse);
        expect(session.choosePermission(allowOnce), isFalse);
        await settle();
        expect(answered, isFalse);
        // The new connection's own requests are the conversation's now, and the old one's are not.
        store.askPermission(request(13), again);
        expect(await store.askPermission(request(14), agent), isA<AcpDeny>());
        expect(session.pendingPermissions.map((q) => q.toolCallId), ['t13']);
        store.dispose();
      },
    );

    test(
      'a tool that finished before the connection ended stays as it was',
      () async {
        final (:store, :session, :agent) = started();
        store.askPermission(request(13), agent);
        session.answerPermission(allow: true);
        session.toolEntries['t13']!.tool!.status = ToolStatus.done;
        store.askPermission(request(14), agent);
        await agent.drop();
        await settle();
        expect(session.toolEntries['t13']!.tool!.status, ToolStatus.done);
        expect(session.toolEntries['t14']!.tool!.status, ToolStatus.stopped);
        store.dispose();
      },
    );

    test('a question is taken away too', () async {
      final (:store, :session, :agent) = started();
      store.askQuestion(_form('s1'), agent);
      final question = session.pendingQuestions.single;
      await agent.drop();
      await settle();
      expect(session.pendingQuestions, isEmpty);
      expect(question.outcome!.how, Settlement.withdrawn);
      store.dispose();
    });
  });

  group('a conversation opened again', () {
    test(
      'shows a tool its earlier run left waiting or running as stopped, one that finished as done, and says nothing is news',
      () async {
        final (:store, :session, :agent) = started();
        final seen = <AttentionKind>[];
        store.attention.listen((a) => seen.add(a.kind));
        final opened = await session.reopen(() async {
          feed(session, {
            'sessionUpdate': 'tool_call',
            'toolCallId': 'w1',
            'title': 'Edit a.txt',
            'kind': 'edit',
            'status': 'pending',
          });
          feed(session, {
            'sessionUpdate': 'tool_call',
            'toolCallId': 'w2',
            'title': 'Run tests',
            'kind': 'execute',
            'status': 'in_progress',
          });
          feed(session, {
            'sessionUpdate': 'tool_call',
            'toolCallId': 'w3',
            'title': 'Read b.txt',
            'kind': 'read',
            'status': 'completed',
          });
          feed(session, state('running'));
          feed(session, state('idle'));
          return AcpOpenedSession('s1', {}, replayed: true);
        });
        expect(opened.replayed, isTrue);
        ToolStatus of(String id) => session.toolEntries[id]!.tool!.status;
        expect(
          (of('w1'), of('w2'), of('w3')),
          (ToolStatus.stopped, ToolStatus.stopped, ToolStatus.done),
        );
        expect(
          session.pendingPermissions,
          isEmpty,
          reason: 'the request itself is not replayed',
        );
        expect(session.opening, isFalse);
        expect(session.isReplaying, isFalse);
        await settle();
        expect(seen, isEmpty, reason: 'a replayed turn is history');
        store.dispose();
      },
    );

    test(
      'says when the agent resumed without sending what was said, and keeps what its answer offered',
      () async {
        final (:store, :session, :agent) = started();
        await session.reopen(
          () async => AcpOpenedSession('s1', {
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
          }),
        );
        expect(
          session.messages.last.text,
          'The agent resumed this without sending what was said before.',
        );
        expect(session.options.map((o) => o.id), ['mode']);
        store.dispose();
      },
    );

    test(
      'that cannot be opened leaves the conversation as it was and says it is not opening',
      () async {
        final (:store, :session, :agent) = started();
        await expectLater(
          session.reopen(() async => throw const AcpFailure('no such session')),
          throwsA(isA<AcpFailure>()),
        );
        expect(session.opening, isFalse);
        expect(session.isReplaying, isFalse);
        store.dispose();
      },
    );
  });
}
