// What a host adds to a conversation of its own: a policy that decides permission requests before the person is asked, hooks that
// tell it what happened, and the few ways it puts its own content in.
import 'dart:async';

import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

Map<String, Object?> _mode(String current, List<String> values) => {
  'id': 'mode',
  'name': 'Mode',
  'category': 'mode',
  'currentValue': current,
  'options': [
    for (final v in values) {'value': v, 'name': v},
  ],
};

Map<String, Object?> _model(String current) => {
  'id': 'model',
  'name': 'Model',
  'category': 'model',
  'currentValue': current,
  'options': [
    for (final v in const ['a', 'b']) {'value': v, 'name': v.toUpperCase()},
  ],
};

void _offer(ChatSession session, [List<Map<String, Object?>>? options]) =>
    session.applyOpened(
      AcpOpenedSession('s1', {
        'configOptions':
            options ??
            [
              _mode('manual', ['manual', 'plan']),
              _model('a'),
            ],
      }),
    );

/// A policy a test steers: what it waits for, what it says of each request and what it answers.
final class _Policy implements PermissionPolicy {
  final prepared = <String>[];
  final reviewed = <String>[];
  Completer<void>? gate;
  PermissionReview Function(AcpPermissionRequest request) onReview = (_) =>
      const PermissionReview();
  PermissionAnswer? Function(AcpPermissionRequest request)? onAnswer;

  @override
  Future<void> prepare(AcpPermissionRequest request) async {
    prepared.add(request.toolCallId);
    final held = gate;
    if (held != null && request.toolCallId == 't1') await held.future;
  }

  @override
  PermissionReview review(
    AcpPermissionRequest request, {
    required bool forPerson,
  }) {
    reviewed.add(request.toolCallId);
    final review = onReview(request);
    return forPerson && !review.forPerson
        ? PermissionReview(forPerson: true, hostData: review.hostData)
        : review;
  }

  @override
  PermissionAnswer? answer(
    AcpPermissionRequest request,
    PermissionReview review,
  ) => onAnswer?.call(request);
}

void main() {
  group('a permission policy', () {
    test(
      'decides requests one after another in the order they came, each read after the one before',
      () async {
        final (:store, :session, :agent) = started();
        final policy = _Policy()..gate = Completer<void>();
        session.permissionPolicy = policy;
        final first = store.askPermission(request(1), agent);
        final second = store.askPermission(request(2), agent);
        await settle();
        expect(policy.prepared, [
          't1',
        ], reason: 'the second waits for the first');
        expect(session.pendingPermissions, isEmpty);
        policy.gate!.complete();
        await settle();
        expect(policy.prepared, ['t1', 't2']);
        expect(
          session.pendingPermissions.map((q) => q.toolCallId),
          ['t1', 't2'],
          reason: 'the cards keep the order the requests came in',
        );
        session.answerPermission(allow: true);
        session.answerPermission(allow: true);
        expect(await first, isA<AcpAllow>());
        expect(await second, isA<AcpAllow>());
        store.dispose();
      },
    );

    test(
      'keeps what it attached on the question, and its words in what is noted',
      () async {
        final (:store, :session, :agent) = started();
        session.permissionPolicy = _Policy()
          ..onReview = (_) => const PermissionReview(
            label: ' for a task',
            hostData: 'rule-subject',
          );
        store.askPermission(request(1), agent);
        await settle();
        expect(session.pendingPermissions.single.hostData, 'rule-subject');
        session.setBypass(true);
        final bypassed = store.askPermission(
          request(2, options: const [allowOnce]),
          agent,
        );
        await settle();
        expect(
          session.messages.last.text,
          'Bypass allowed: Edit a.txt for a task (Yes).',
        );
        expect(await bypassed, isA<AcpSelect>());
        store.dispose();
      },
    );

    test(
      'answers a request itself, after Bypass, and notes it; one it marked for the person alone is never answered by it',
      () async {
        final (:store, :session, :agent) = started();
        final policy = _Policy()
          ..onAnswer = (request) => request.toolCallId == 't3'
              ? null
              : const PermissionAnswer(
                  AcpSelect('allow-once'),
                  'A rule allowed it.',
                );
        session.permissionPolicy = policy;
        final answered = await store.askPermission(request(1), agent);
        expect(answered, isA<AcpSelect>());
        expect(session.messages.last.text, 'A rule allowed it.');
        expect(session.pendingPermissions, isEmpty);
        policy.onReview = (_) => const PermissionReview(forPerson: true);
        store.askPermission(request(2), agent);
        await settle();
        expect(session.pendingPermissions.single.toolCallId, 't2');
        expect(session.pendingPermissions.single.forPerson, isTrue);
        policy.onReview = (_) => const PermissionReview();
        store.askPermission(request(3), agent);
        await settle();
        expect(
          session.pendingPermissions.map((q) => q.toolCallId),
          ['t2', 't3'],
          reason: 'the policy declined it, so it waits',
        );
        store.dispose();
      },
    );

    test(
      'Bypass answers before the policy is asked, and a request for the person alone is not Bypass\'s',
      () async {
        final (:store, :session, :agent) = started();
        var asked = 0;
        session.permissionPolicy = _Policy()
          ..onAnswer = (_) {
            asked++;
            return null;
          };
        session.setBypass(true);
        expect(await store.askPermission(request(1), agent), isA<AcpSelect>());
        expect(asked, 0);
        (session.permissionPolicy as _Policy).onReview = (_) =>
            const PermissionReview(forPerson: true);
        store.askPermission(request(2), agent);
        await settle();
        expect(session.pendingPermissions.single.bypassPassed, isTrue);
        store.dispose();
      },
    );

    test(
      'a tool call that is another thread\'s does not show as this one\'s',
      () async {
        final (:store, :session, :agent) = started();
        session.permissionPolicy = _Policy()
          ..onReview = (_) => const PermissionReview(ownCall: false);
        store.askPermission(request(1), agent);
        await settle();
        expect(session.pendingPermissions, hasLength(1));
        expect(session.toolEntries, isEmpty);
        store.dispose();
      },
    );

    test(
      'Stop answers a request still waiting on it cancelled, and it is never decided',
      () async {
        final (:store, :session, :agent) = started();
        final policy = _Policy()..gate = Completer<void>();
        session.permissionPolicy = policy;
        final waiting = store.askPermission(request(1), agent);
        final behind = store.askPermission(request(2), agent);
        await settle();
        await session.cancel();
        expect(await waiting, isA<AcpCancel>());
        expect(await behind, isA<AcpCancel>());
        policy.gate!.complete();
        await settle();
        expect(policy.reviewed, isEmpty);
        expect(session.pendingPermissions, isEmpty);
        store.dispose();
      },
    );

    test('a connection that ended while a request waited cancels it', () async {
      final (:store, :session, :agent) = started();
      final policy = _Policy()..gate = Completer<void>();
      session.permissionPolicy = policy;
      final waiting = store.askPermission(request(1), agent);
      await settle();
      session.detach();
      expect(await waiting, isA<AcpCancel>());
      policy.gate!.complete();
      await settle();
      expect(policy.reviewed, isEmpty);
      store.dispose();
    });

    test('a failing policy fails that request and lets the next go', () async {
      final (:store, :session, :agent) = started();
      final policy = _Policy()
        ..onReview = (request) => request.toolCallId == 't1'
            ? throw StateError('no rules')
            : const PermissionReview();
      session.permissionPolicy = policy;
      final first = store.askPermission(request(1), agent);
      final second = store.askPermission(request(2), agent);
      await expectLater(first, throwsStateError);
      await settle();
      expect(session.pendingPermissions.single.toolCallId, 't2');
      session.answerPermission(allow: true);
      expect(await second, isA<AcpAllow>());
      store.dispose();
    });

    test('told as each request settles, with who answered', () async {
      final (:store, :session, :agent) = started();
      final settled = <String?>[];
      session.onPermissionSettled = (question) =>
          settled.add('${question.toolCallId}:${question.outcome!.how.name}');
      store.askPermission(
        request(1, options: const [allowOnce, allowAlways]),
        agent,
      );
      session.choosePermission(allowAlways);
      store.askPermission(request(2), agent);
      await session.cancel();
      expect(settled, ['t1:answered', 't2:cancelled']);
      store.dispose();
    });
  });

  group('hooks', () {
    test(
      'onPrompt is told of what is handed to the agent, after the earlier choices went',
      () async {
        final (:store, :session, :agent) = started();
        _offer(session);
        final told = <String>[];
        session.onPrompt = (text, attachments) {
          told.add('$text/${attachments.length}/${agent.calls.join(',')}');
        };
        session.holdChoice(const PendingChoice('model', OptionRole.model, 'b'));
        await session.send('go');
        expect(told, ['go/0/config:model=b']);
        store.dispose();
      },
    );

    test(
      'onChoice is told of a choice a person made that the agent took, now or later, and not of a default',
      () async {
        final (:store, :session, :agent) = started();
        _offer(session);
        final told = <String>[];
        session.onChoice = (role, value) => told.add('${role.name}=$value');
        await session.choose(optionIn(session.options, OptionRole.model)!, 'b');
        expect(told, ['model=b']);
        session.holdChoice(
          const PendingChoice('mode', OptionRole.mode, 'plan'),
        );
        await session.send('go');
        expect(told, [
          'model=b',
        ], reason: 'a default a host put there is not a choice');
        feed(session, state('running'));
        await session.choose(
          optionIn(session.options, OptionRole.mode)!,
          'manual',
        );
        expect(told, ['model=b'], reason: 'it waits for the turn to end');
        await session.send('next');
        feed(session, state('idle', reason: 'end_turn'));
        await settle();
        expect(told, ['model=b', 'mode=manual']);
        store.dispose();
      },
    );

    test(
      'a choice made with no connection is held for the agent that comes',
      () async {
        final session = ChatSession(id: 'x', sessionId: 's1');
        _offer(session);
        expect(
          await session.choose(
            optionIn(session.options, OptionRole.model)!,
            'b',
          ),
          isFalse,
        );
        expect(session.pendingChoices['model']!.byPerson, isTrue);
        final agent = FakeConnection();
        session.attach(agent);
        await session.send('go');
        expect(agent.calls, ['config:model=b', 'prompt:go']);
      },
    );

    test(
      'onOptions and onTitle are told of what the agent says by itself',
      () async {
        final (:store, :session, :agent) = started();
        var options = 0;
        final titles = <String>[];
        session.onOptions = () => options++;
        session.onTitle = titles.add;
        feed(session, {
          'sessionUpdate': 'config_option_update',
          'configOptions': [_model('b')],
        });
        expect(options, 1);
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Fix it',
        });
        expect(titles, ['Fix it']);
        session.rename('Mine');
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Again',
        });
        expect(titles, ['Fix it'], reason: 'the person named it');
        store.dispose();
      },
    );

    test(
      'a mode the agent moved to is told, and kept as chosen only while agentModeIsChosen',
      () async {
        final (:store, :session, :agent) = started();
        _offer(session);
        final moved = <String>[];
        final kept = <String?>[];
        session.onAgentMode = (before, now) => moved.add('$before>$now');
        session.onModeKept = (mode, bypass) => kept.add(mode);
        feed(session, {
          'sessionUpdate': 'config_option_update',
          'configOptions': [
            _mode('plan', ['manual', 'plan']),
          ],
        });
        expect(moved, ['manual>plan']);
        expect(session.chosenMode, 'plan');
        session.agentModeIsChosen = false;
        feed(session, {
          'sessionUpdate': 'config_option_update',
          'configOptions': [
            _mode('manual', ['manual', 'plan']),
          ],
        });
        expect(moved, ['manual>plan', 'plan>manual']);
        expect(session.chosenMode, 'plan', reason: 'shown, not chosen');
        store.dispose();
      },
    );

    test(
      'a mode left or chosen with no connection is still told to onModeKept',
      () async {
        final (:store, :session, :agent) = started();
        _offer(session);
        session.setBypass(true);
        final kept = <(String?, bool)>[];
        session.onModeKept = (mode, bypass) => kept.add((mode, bypass));
        session.detach();
        await session.leaveBypass();
        expect(session.bypass, isFalse);
        expect(
          kept,
          [(null, false)],
          reason:
              'the person was told Bypass is off, so a reopen must not put it back',
        );
        store.dispose();
      },
    );

    test('modeLabel words the modes the conversation cycles to', () async {
      final (:store, :session, :agent) = started();
      _offer(session, [
        _mode('plan', ['manual', 'plan']),
      ]);
      session.modeLabel = (choice) => choice.value.toUpperCase();
      session.setBypass(true);
      await session.cycleMode();
      expect(session.bypass, isFalse);
      expect(
        session.messages.last.text,
        'Mode: MANUAL. Requests are asked again.',
      );
      store.dispose();
    });
  });

  group('the host\'s own content', () {
    test(
      'a note is the chat\'s word, a message is the host\'s, and a title can change while the agent still names it',
      () {
        final session = ChatSession(id: 'x', title: 'New session');
        final at = DateTime.utc(2026, 1, 2, 3, 4);
        session.note('Not sent.');
        session.addMessage(MessageRole.user, 'kept', at: at);
        expect(session.messages.map((m) => m.role), [
          MessageRole.notice,
          MessageRole.user,
        ]);
        expect(session.messages.last.at, at);
        session.retitle('From the first message');
        expect(session.title, 'From the first message');
        expect(session.agentNames, isTrue);
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Named',
        });
        expect(session.title, 'Named');
        session.rename('Mine');
        session.agentNames = true;
        feed(session, {
          'sessionUpdate': 'session_info_update',
          'title': 'Again',
        });
        expect(
          session.title,
          'Again',
          reason: 'the host let the agent name it again',
        );
      },
    );

    test('messages the host put there can be taken out again', () {
      final session = ChatSession(id: 'x');
      session
        ..addMessage(MessageRole.user, 'a')
        ..addMessage(MessageRole.agent, 'b')
        ..addMessage(MessageRole.notice, 'c');
      session.removeMessages(
        (m) => m.role == MessageRole.user || m.role == MessageRole.agent,
      );
      expect(session.messages.map((m) => m.text), ['c']);
    });

    test(
      'Bypass can be preset without a word, and choices waiting can be taken back',
      () {
        final session = ChatSession(id: 'x');
        session.presetBypass(true);
        expect(session.bypass, isTrue);
        expect(session.messages, isEmpty);
        session
          ..holdChoice(const PendingChoice('model', OptionRole.model, 'b'))
          ..holdChoice(const PendingChoice('mode', OptionRole.mode, 'plan'));
        session.dropChoices(where: (choice) => choice.role == OptionRole.mode);
        expect(session.pendingChoices.keys, ['model']);
        session.dropChoices();
        expect(session.pendingChoices, isEmpty);
      },
    );
  });
}
