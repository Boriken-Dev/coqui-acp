// Points a review of the model raised, each held by a test: a request is never left waiting for an answer nobody can give, state
// is read through views, text is cut between characters and grows without copying itself, and a change in a listener's list during
// a notification is respected.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

AcpElicitation _form() =>
    const AcpElicitation(sessionId: 's1', message: 'Which?', fields: []);

void main() {
  group('what is asked is never left waiting for an answer nobody can give', () {
    test(
      'when the conversation is let go (detach, forget, dispose), each request is answered cancelled and labelled withdrawn',
      () async {
        for (final how in ['detach', 'forget', 'dispose', 'attach']) {
          final (:store, :session, :agent) = started();
          final permission = store.askPermission(request(1), agent);
          final question = store.askQuestion(_form(), agent);
          final asked = session.pendingPermissions.single;
          final form = session.pendingQuestions.single;
          switch (how) {
            case 'detach':
              session.detach();
            case 'forget':
              store.forget(session);
            case 'dispose':
              session.dispose();
            case 'attach':
              session.attach(FakeConnection());
          }
          expect(
            await permission.timeout(const Duration(seconds: 1)),
            isA<AcpCancel>(),
            reason: how,
          );
          expect(
            await question.timeout(const Duration(seconds: 1)),
            isA<AcpElicitCancel>(),
            reason: how,
          );
          expect(asked.outcome!.how, Settlement.withdrawn, reason: how);
          expect(form.outcome!.how, Settlement.withdrawn, reason: how);
          expect(
            session.toolEntries['t1']!.tool!.status,
            ToolStatus.stopped,
            reason: how,
          );
          store.dispose();
        }
      },
    );

    test(
      'Bypass answers a request that offers an allow even when the conversation holds none waiting',
      () async {
        final (:store, :session, :agent) = started(
          limits: const ChatLimits(pendingPermissions: 0),
        );
        session.setBypass(true);
        expect(await store.askPermission(request(1), agent), isA<AcpSelect>());
        expect(
          await store.askPermission(
            request(2, options: const [rejectOnce]),
            agent,
          ),
          isA<AcpCancel>(),
        );
        expect(
          session.messages.last.text,
          contains('more requests than the 0'),
        );
        store.dispose();
      },
    );
  });

  group('state is read through views', () {
    test('and changed only by methods', () async {
      final (:store, :session, :agent) = started();
      store.askPermission(request(1), agent);
      await session.send('go');
      feed(session, state('running'));
      await session.send('queued');
      session.holdChoice(const PendingChoice('model', OptionRole.model, 'a'));
      void throws(void Function() change) =>
          expect(change, throwsUnsupportedError);
      throws(() => session.messages.clear());
      throws(() => session.queue.clear());
      throws(() => session.pendingPermissions.clear());
      throws(() => session.pendingQuestions.removeWhere((_) => true));
      throws(() => session.toolEntries.clear());
      throws(() => session.terminals.clear());
      throws(() => session.compactions.clear());
      throws(() => session.pendingChoices.clear());
      throws(() => session.turns.clear());
      throws(() => session.settledPermissions.clear());
      throws(() => session.clients.clear());
      throws(() => session.refusedModes.add('x'));
      expect(session.messages, isNotEmpty);
      store.dispose();
    });
  });

  group('text', () {
    test('is cut between characters, never through one', () {
      const smile = '\u{1F600}';
      final session = ChatSession(
        id: 'x',
        sessionId: 's1',
        limits: const ChatLimits(
          shortText: 3,
          messageText: 3,
          terminalOutput: 2,
        ),
      );
      feed(session, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 't',
        'title': 'ab${smile}cd',
      });
      expect(session.toolEntries['t']!.tool!.title, 'ab…');
      feed(session, text('agent_message', 'ab$smile', id: 'm'));
      expect(session.messages.last.text, 'ab\n…');
      feed(session, {
        'sessionUpdate': 'terminal_update',
        'terminalId': 'x',
        'output': {'data': 'YfCfmIBi'},
      });
      expect(
        session.terminals['x']!.output,
        'b',
        reason:
            'the last two code units of a, the smile and b start inside the smile',
      );
      expect(
        titleFrom('ab${smile}cd', const ChatLimits(titleLength: 4)),
        'ab…',
      );
    });

    test(
      'grows by appending without copying all of it each time',
      () {
        final session = ChatSession(id: 'x', sessionId: 's1');
        final piece = 'x' * 100;
        for (var i = 0; i < 40000; i++) {
          feed(session, text('agent_message_chunk', piece));
        }
        expect(session.messages.single.text.length, 2 * 1024 * 1024 + 2);
        expect(session.messages.single.text.endsWith('\n…'), isTrue);
        final terminal = ChatSession(id: 'y', sessionId: 's1');
        for (var i = 0; i < 40000; i++) {
          feed(terminal, {
            'sessionUpdate': 'terminal_output_chunk',
            'terminalId': 'x',
            'data': 'eHh4eHh4eHh4eA==',
          });
        }
        expect(
          terminal.terminals['x']!.output.length,
          lessThanOrEqualTo(256 * 1024),
        );
        expect(terminal.terminals['x']!.output.length, greaterThan(200 * 1024));
      },
      timeout: const Timeout(Duration(seconds: 20)),
    );

    test('a compaction summary grows the same way, from its start', () {
      final session = ChatSession(
        id: 'x',
        sessionId: 's1',
        limits: const ChatLimits(toolText: 10),
      );
      for (var i = 0; i < 5; i++) {
        feed(session, {
          'sessionUpdate': 'compaction_summary_chunk',
          'compactionId': 'c',
          'content': {'type': 'text', 'text': 'abcd'},
        });
      }
      expect(session.messages.single.compaction!.summary, 'abcdabcdab\n…');
    });
  });

  group('names of what went with a message', () {
    test(
      'are remembered up to the limit, and a message whose name was forgotten is named again',
      () {
        final session = ChatSession(
          id: 'x',
          sessionId: 's1',
          limits: const ChatLimits(entries: 3),
        );
        Map<String, Object?> sent(String id) => {
          'sessionUpdate': 'user_message',
          'messageId': id,
          'content': [
            {
              'type': 'resource_link',
              'name': 'f$id.txt',
              'uri': 'file:///w/f$id.txt',
            },
          ],
        };
        feed(session, sent('m1'));
        feed(session, sent('m1'));
        expect(
          session.messages.where((m) => m.text == 'Sent with this: fm1.txt'),
          hasLength(1),
        );
        for (final id in ['m2', 'm3', 'm4', 'm5']) {
          feed(session, sent(id));
        }
        feed(session, sent('m1'));
        expect(
          session.messages.last.text,
          'Sent with this: fm1.txt',
          reason: 'its name was forgotten, so it is named again',
        );
      },
    );
  });

  group('listeners', () {
    test(
      'one removed by an earlier listener during a notification is not told',
      () {
        final notifier = ChatNotifier();
        var heard = 0;
        late void Function() second;
        void first() => notifier.removeListener(second);
        second = () => heard++;
        notifier
          ..addListener(first)
          ..addListener(second);
        notifier.notifyListeners();
        expect(heard, 0);
      },
    );
  });

  group('the attention a conversation needs', () {
    test(
      'is on the conversation before its listeners are told, so one that looks sees it',
      () {
        final store = ChatStore();
        final behind = store.add(title: 'Behind', agent: 'A', sessionId: 's1');
        store.add(title: 'In view', agent: 'A', sessionId: 's2');
        bool? seen;
        store.addListener(() => seen = behind.unread);
        feed(behind, state('running'));
        feed(behind, state('idle', reason: 'end_turn'));
        expect(behind.unread, isTrue, reason: 'at once, not a moment later');
        expect(seen, isTrue);
        store.dispose();
      },
    );
  });
}
