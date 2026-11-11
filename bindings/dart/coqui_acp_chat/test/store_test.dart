// The conversations of one window: the chosen one, what needs the person, and a turn finished behind the one in view.
import 'package:coqui_acp/coqui_acp.dart';
import 'package:coqui_acp_chat/coqui_acp_chat.dart';
import 'package:test/test.dart';

import 'support/fakes.dart';

void main() {
  test(
    'the newest conversation is chosen, choosing another makes it so, and a closed one hands over to the first',
    () {
      final store = ChatStore();
      final first = store.add(title: 'First', agent: 'A', sessionId: 's1');
      final second = store.add(title: 'Second', agent: 'A', sessionId: 's2');
      expect(store.selected, same(second));
      expect(store.sessions, [second, first]);
      store.select(first.id);
      expect(store.selected?.title, 'First');
      store.select('nothing');
      expect(store.selectedId, first.id);
      store.forget(first);
      expect(store.selected, same(second));
      store.dispose();
    },
  );

  test("a change in a conversation tells the store's listeners", () {
    final store = ChatStore();
    final session = store.add(title: 'T', agent: 'A', sessionId: 's1');
    var heard = 0;
    store.addListener(() => heard++);
    feed(session, text('agent_message', 'hi', id: 'm'));
    expect(heard, 1);
    session.rename('Mine');
    expect(heard, 2);
    store.dispose();
  });

  test(
    'a request behind is told by its kind, never by what it would do, and counted as waiting',
    () async {
      final store = ChatStore();
      final behind = store.add(title: 'Behind', agent: 'A', sessionId: 's1');
      final agent = FakeConnection();
      behind.attach(agent);
      store.add(title: 'In view', agent: 'A', sessionId: 's2');
      final told = <Attention>[];
      store.attention.listen(told.add);
      store.askPermission(request(1), agent);
      store.askQuestion(
        AcpElicitation(sessionId: 's1', message: 'Which?', fields: const []),
        agent,
      );
      await settle();
      expect(told.map((a) => (a.session.title, a.kind)), [
        ('Behind', AttentionKind.permission),
        ('Behind', AttentionKind.question),
      ]);
      expect(store.waiting, 1);
      behind.answerPermission(allow: true);
      behind.answerQuestion(const AcpElicitCancel());
      expect(store.waiting, 0);
      store.dispose();
    },
  );

  test(
    'a finished turn behind is told and marked new until the conversation is chosen; the one in view is not marked',
    () async {
      final store = ChatStore();
      final behind = store.add(title: 'Behind', agent: 'A', sessionId: 's1');
      final inView = store.add(title: 'In view', agent: 'A', sessionId: 's2');
      final told = <Attention>[];
      store.attention.listen(told.add);
      feed(behind, state('running'));
      feed(behind, state('idle', reason: 'end_turn'));
      feed(inView, state('running'));
      feed(inView, state('idle', reason: 'end_turn'));
      await settle();
      expect(told.map((a) => (a.session.title, a.kind)), [
        ('Behind', AttentionKind.finished),
        ('In view', AttentionKind.finished),
      ]);
      expect(behind.unread, isTrue);
      expect(inView.unread, isFalse);
      store.select(behind.id);
      expect(behind.unread, isFalse);
      store.dispose();
    },
  );

  test('a turn the person stopped is not news to them', () async {
    final store = ChatStore();
    final session = store.add(title: 'T', agent: 'A', sessionId: 's1');
    final told = <Attention>[];
    store.attention.listen(told.add);
    feed(session, state('running'));
    feed(session, state('idle', reason: 'cancelled'));
    await settle();
    expect(told, isEmpty);
    store.dispose();
  });

  test(
    'closing a conversation answers what it held cancelled and ends the connection once nothing else uses it',
    () async {
      final store = ChatStore();
      final agent = FakeConnection();
      final one = store.add(title: 'One', agent: 'A', sessionId: 's1')
        ..attach(agent);
      final two = store.add(title: 'Two', agent: 'A', sessionId: 's2')
        ..attach(agent);
      final answer = store.askPermission(request(1), agent);
      await store.close(one);
      expect(await answer, isA<AcpCancel>());
      expect(
        agent.closed,
        isFalse,
        reason: 'another conversation still uses it',
      );
      expect(store.sessions, [two]);
      await store.close(two);
      expect(agent.closed, isTrue);
      expect(store.selected, isNull);
      store.dispose();
    },
  );

  test('forgetting leaves the connection with whoever else uses it', () async {
    final (:store, :session, :agent) = started();
    store.forget(session);
    expect(agent.closed, isFalse);
    expect(session.connection, isNull);
    store.dispose();
  });

  test(
    'a closed connection cannot be attached, nor can a conversation with no session id',
    () async {
      final store = ChatStore();
      final agent = FakeConnection();
      await agent.close();
      expect(
        () => store.add(title: 'T', agent: 'A', sessionId: 's1').attach(agent),
        throwsStateError,
      );
      expect(
        () => store.add(title: 'T', agent: 'A').attach(FakeConnection()),
        throwsStateError,
      );
      store.dispose();
    },
  );
}
